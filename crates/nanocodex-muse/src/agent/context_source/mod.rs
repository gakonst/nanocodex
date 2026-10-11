#[cfg(not(target_family = "wasm"))]
mod agents_md;

#[cfg(not(target_family = "wasm"))]
#[path = "native.rs"]
mod platform;

#[cfg(all(target_family = "wasm", target_os = "unknown"))]
#[path = "web.rs"]
mod platform;

/// Model-visible facts owned by a remote tool-execution environment.
///
/// Configure this when tools operate somewhere other than the embedding
/// process. Without it, native agents discover project instructions, date,
/// and timezone from the embedding host.
#[derive(Clone, Debug)]
pub struct ExecutionEnvironment {
    pub(crate) current_date: std::sync::Arc<str>,
    pub(crate) timezone: std::sync::Arc<str>,
    pub(crate) project_instructions: Option<std::sync::Arc<str>>,
}

impl ExecutionEnvironment {
    /// Creates an environment with no project-instruction snapshot.
    ///
    /// The date must use `YYYY-MM-DD`; the timezone should be the name exposed
    /// by the execution environment, such as `Etc/UTC`. Invalid values are
    /// rejected when the agent is built. Omitting project instructions means
    /// that the remote workspace has none and suppresses host discovery.
    #[must_use]
    pub fn new(
        current_date: impl Into<std::sync::Arc<str>>,
        timezone: impl Into<std::sync::Arc<str>>,
    ) -> Self {
        Self {
            current_date: current_date.into(),
            timezone: timezone.into(),
            project_instructions: None,
        }
    }

    /// Adds the complete project-instruction snapshot visible in the remote
    /// workspace. An empty snapshot is rejected; omit this method when the
    /// remote workspace has no project instructions.
    #[must_use]
    pub fn project_instructions(mut self, instructions: impl Into<std::sync::Arc<str>>) -> Self {
        self.project_instructions = Some(instructions.into());
        self
    }
}

pub(crate) use platform::ContextSource;
pub(super) use platform::ContextSourceConfig;
