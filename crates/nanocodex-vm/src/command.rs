use std::{collections::BTreeMap, ffi::OsString, fmt, path::Path};

use serde::{Deserialize, Serialize};

const DEFAULT_PATH: &str = "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin";

/// A command to execute as the initial process inside a libkrun guest.
#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
pub struct GuestCommand {
    program: OsString,
    arguments: Vec<OsString>,
    #[serde(with = "environment_serde")]
    environment: BTreeMap<OsString, OsString>,
    current_dir: OsString,
}

impl fmt::Debug for GuestCommand {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("GuestCommand")
            .field("program", &self.program)
            .field("arguments", &self.arguments)
            .field(
                "environment_keys",
                &self.environment.keys().collect::<Vec<_>>(),
            )
            .field("current_dir", &self.current_dir)
            .finish()
    }
}

impl GuestCommand {
    /// Creates a guest command with `/` as its working directory and a
    /// conventional system `PATH`.
    pub fn new(program: impl Into<OsString>) -> Self {
        Self {
            program: program.into(),
            arguments: Vec::new(),
            environment: BTreeMap::from([(OsString::from("PATH"), OsString::from(DEFAULT_PATH))]),
            current_dir: OsString::from("/"),
        }
    }

    /// Appends one argument.
    #[must_use]
    pub fn arg(mut self, argument: impl Into<OsString>) -> Self {
        self.arguments.push(argument.into());
        self
    }

    /// Appends arguments in order.
    #[must_use]
    pub fn args<I, A>(mut self, arguments: I) -> Self
    where
        I: IntoIterator<Item = A>,
        A: Into<OsString>,
    {
        self.arguments.extend(arguments.into_iter().map(Into::into));
        self
    }

    /// Sets or replaces one environment variable.
    #[must_use]
    pub fn env(mut self, name: impl Into<OsString>, value: impl Into<OsString>) -> Self {
        self.environment.insert(name.into(), value.into());
        self
    }

    /// Sets the absolute guest working directory.
    #[must_use]
    pub fn current_dir(mut self, directory: impl Into<OsString>) -> Self {
        self.current_dir = directory.into();
        self
    }

    /// Returns the guest executable.
    #[must_use]
    pub fn program(&self) -> &Path {
        Path::new(&self.program)
    }

    /// Returns the ordered guest arguments.
    #[must_use]
    pub fn arguments(&self) -> &[OsString] {
        &self.arguments
    }

    /// Returns the complete guest environment.
    #[must_use]
    pub const fn environment(&self) -> &BTreeMap<OsString, OsString> {
        &self.environment
    }

    /// Returns the guest working directory.
    #[must_use]
    pub fn current_directory(&self) -> &Path {
        Path::new(&self.current_dir)
    }
}

mod environment_serde {
    use std::{collections::BTreeMap, ffi::OsString};

    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    pub fn serialize<S>(
        environment: &BTreeMap<OsString, OsString>,
        serializer: S,
    ) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        environment.iter().collect::<Vec<_>>().serialize(serializer)
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<BTreeMap<OsString, OsString>, D::Error>
    where
        D: Deserializer<'de>,
    {
        Ok(Vec::<(OsString, OsString)>::deserialize(deserializer)?
            .into_iter()
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_owns_guest_process_policy() {
        let command = GuestCommand::new("/bin/sh")
            .args(["-c", "pwd"])
            .env("TERM", "dumb")
            .current_dir("/workspace");

        assert_eq!(command.program(), Path::new("/bin/sh"));
        assert_eq!(
            command.arguments(),
            [OsString::from("-c"), OsString::from("pwd")]
        );
        assert_eq!(
            command.environment().get(&OsString::from("TERM")),
            Some(&OsString::from("dumb"))
        );
        assert_eq!(command.current_directory(), Path::new("/workspace"));
    }
}
