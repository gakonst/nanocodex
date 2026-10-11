use nanocodex_oai_api::responses::ResponseItem;

#[cfg(feature = "openai")]
use crate::session::CommittedSession;

/// Read-only model context exposed to session adapters.
///
/// The history is complete and unredacted and can contain reasoning and tool
/// inputs or outputs. Applications must protect it like a session snapshot.
#[derive(Clone, Debug)]
pub struct AgentSessionContext {
    workspace: String,
    history: Vec<ResponseItem>,
}

impl AgentSessionContext {
    #[cfg(feature = "openai")]
    pub(super) fn new(checkpoint: Option<&CommittedSession>, workspace: String) -> Self {
        let history =
            checkpoint.map_or_else(Vec::new, |checkpoint| checkpoint.model().snapshot_history());
        Self { workspace, history }
    }

    /// Constructs a context snapshot observed by an external lifecycle backend.
    #[doc(hidden)]
    #[must_use]
    pub const fn from_backend(workspace: String, history: Vec<ResponseItem>) -> Self {
        Self { workspace, history }
    }

    /// Returns the absolute workspace owned by the agent session.
    #[must_use]
    pub fn workspace(&self) -> &str {
        &self.workspace
    }

    /// Returns complete model-visible history at the latest safe boundary.
    #[must_use]
    pub fn history(&self) -> &[ResponseItem] {
        &self.history
    }
}
