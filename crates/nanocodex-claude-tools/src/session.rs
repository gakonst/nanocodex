//! Session identity exported to every tool subprocess.
//!
//! Mirrors `nanocodex_oai_tools::SessionEnvironment`: Claude Bash executors,
//! PDF helpers, and any other process a Claude tool launches receive the same
//! two variables as Codex tools, so a subprocess observes its launching
//! session independent of the agent harness that owns it.

use std::sync::Arc;

/// Identity of the agent session that launches a tool subprocess.
///
/// Every process a tool spawns receives [`Self::SESSION_ID_VAR`]
/// (`CODEX_THREAD_ID`, compatible with Codex CLI) set to the launching
/// session and [`Self::ROOT_SESSION_ID_VAR`] set to the root of its
/// conversation tree. Roots export their own id for both. Forks, side
/// conversations, and subagents export their own session id and the shared
/// root id. Values bound here override same-named caller or ambient values.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct SessionEnvironment {
    session_id: Arc<str>,
    root_session_id: Arc<str>,
}

impl SessionEnvironment {
    /// Variable holding the launching session id.
    pub const SESSION_ID_VAR: &'static str = "CODEX_THREAD_ID";
    /// Variable holding the root session id of the launching conversation tree.
    pub const ROOT_SESSION_ID_VAR: &'static str = "NANOCODEX_ROOT_SESSION_ID";
    /// Every variable this type controls.
    pub const VARIABLES: [&'static str; 2] = [Self::SESSION_ID_VAR, Self::ROOT_SESSION_ID_VAR];

    /// Identity of `session_id`, which belongs to the tree rooted at `root_session_id`.
    #[must_use]
    pub fn new(session_id: impl AsRef<str>, root_session_id: impl AsRef<str>) -> Self {
        Self {
            session_id: Arc::from(session_id.as_ref()),
            root_session_id: Arc::from(root_session_id.as_ref()),
        }
    }

    /// Identity of a root session.
    #[must_use]
    pub fn root(session_id: impl AsRef<str>) -> Self {
        let session_id: Arc<str> = Arc::from(session_id.as_ref());
        Self {
            root_session_id: Arc::clone(&session_id),
            session_id,
        }
    }

    /// Launching session id.
    #[must_use]
    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    /// Root session id of the launching conversation tree.
    #[must_use]
    pub fn root_session_id(&self) -> &str {
        &self.root_session_id
    }

    /// Variable assignments to export, in a stable order.
    #[must_use]
    pub fn variables(&self) -> [(&'static str, &str); 2] {
        [
            (Self::SESSION_ID_VAR, &self.session_id),
            (Self::ROOT_SESSION_ID_VAR, &self.root_session_id),
        ]
    }

    /// Returns whether `name` is controlled by this type.
    #[must_use]
    pub fn controls(name: &std::ffi::OsStr) -> bool {
        Self::VARIABLES.iter().any(|candidate| name == *candidate)
    }

    /// Applies this identity to a command, overriding caller or inherited values.
    #[cfg(not(target_family = "wasm"))]
    pub fn apply(&self, command: &mut std::process::Command) {
        command.envs(self.variables());
    }

    /// Removes inherited session variables from a command launched outside any session.
    #[cfg(not(target_family = "wasm"))]
    pub fn clear(command: &mut std::process::Command) {
        for name in Self::VARIABLES {
            command.env_remove(name);
        }
    }

    /// Applies `session` when bound, otherwise removes inherited identities so
    /// a subprocess never observes the launching process's own session.
    #[cfg(not(target_family = "wasm"))]
    pub fn apply_or_clear(session: Option<&Self>, command: &mut std::process::Command) {
        match session {
            Some(session) => session.apply(command),
            None => Self::clear(command),
        }
    }
}
