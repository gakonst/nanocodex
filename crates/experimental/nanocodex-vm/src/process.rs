use std::{
    fs::File,
    io::{self, BufReader, BufWriter, Write},
    path::Path,
};

use serde::{Deserialize, Serialize};
use tempfile::NamedTempFile;
use thiserror::Error;

use crate::{
    command::GuestCommand,
    config::VmConfig,
    krun::{KrunVm, VmError},
};

const PROCESS_CONFIG_VERSION: u32 = 2;

/// Complete owned input for one dedicated VMM process.
///
/// Keeping guest environment values in this file avoids exposing proxy
/// credentials and secret placeholders through the VMM's process arguments.
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct VmProcessConfig {
    version: u32,
    vm: VmConfig,
    command: GuestCommand,
}

impl VmProcessConfig {
    /// Creates a versioned launch record from complete owned VM inputs.
    #[must_use]
    pub const fn new(vm: VmConfig, command: GuestCommand) -> Self {
        Self {
            version: PROCESS_CONFIG_VERSION,
            vm,
            command,
        }
    }

    /// Writes this configuration to a mode-0600 temporary file.
    ///
    /// # Errors
    ///
    /// Returns an error when the private file cannot be created or serialized.
    pub fn write_private(&self) -> Result<PrivateVmProcessConfig, VmProcessError> {
        let mut file = NamedTempFile::new()?;
        let mut writer = BufWriter::new(file.as_file_mut());
        serde_json::to_writer(&mut writer, self)?;
        writer.flush()?;
        drop(writer);
        file.as_file_mut().flush()?;
        Ok(PrivateVmProcessConfig { file })
    }

    /// Reads a process configuration created by [`Self::write_private`].
    ///
    /// # Errors
    ///
    /// Returns an error when the file cannot be read, has an unsupported
    /// version, or contains an invalid configuration.
    pub fn read(path: impl AsRef<Path>) -> Result<Self, VmProcessError> {
        let config: Self = serde_json::from_reader(BufReader::new(File::open(path)?))?;
        if config.version != PROCESS_CONFIG_VERSION {
            return Err(VmProcessError::UnsupportedVersion(config.version));
        }
        Ok(config)
    }

    /// Enters the blocking libkrun loop described by this configuration.
    ///
    /// # Errors
    ///
    /// Returns an error when VM construction or startup fails.
    pub fn run(self) -> Result<(), VmError> {
        KrunVm::new(&self.vm)?.run(&self.command)
    }
}

/// Owned private file kept alive until its VMM child has loaded it.
pub struct PrivateVmProcessConfig {
    file: NamedTempFile,
}

impl PrivateVmProcessConfig {
    /// Returns the mode-0600 temporary configuration path.
    #[must_use]
    pub fn path(&self) -> &Path {
        self.file.path()
    }
}

/// Failure to persist or load a private VMM launch record.
#[derive(Debug, Error)]
pub enum VmProcessError {
    /// Private configuration file I/O failed.
    #[error("failed to access the private VM process configuration: {0}")]
    Io(#[from] io::Error),
    /// Private configuration serialization failed.
    #[error("failed to encode the private VM process configuration: {0}")]
    Json(#[from] serde_json::Error),
    /// The file uses a launch-record version this crate does not understand.
    #[error("unsupported VM process configuration version {0}")]
    UnsupportedVersion(u32),
}

#[cfg(test)]
mod tests {
    use std::{ffi::OsString, os::unix::fs::PermissionsExt};

    use super::*;
    use crate::config::Network;

    #[test]
    fn private_config_round_trips_without_argv_delivery() {
        let command = GuestCommand::new("/bin/true")
            .env("HTTPS_PROXY", "http://lease:credential@host.internal:8080");
        let private = VmProcessConfig::new(
            VmConfig::ext4("/tmp/rootfs.ext4").network(Network::Disabled),
            command,
        )
        .write_private()
        .unwrap();
        let permissions = private.path().metadata().unwrap().permissions().mode();
        assert_eq!(permissions & 0o077, 0);

        let decoded = VmProcessConfig::read(private.path()).unwrap();
        assert_eq!(
            decoded
                .command
                .environment()
                .get(&OsString::from("HTTPS_PROXY"))
                .unwrap(),
            &OsString::from("http://lease:credential@host.internal:8080")
        );
    }
}
