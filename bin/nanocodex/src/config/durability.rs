//! Durability for the CLI's existing mixed-family recipes and child registry.
use super::*;
use nanocodex::agent::ChildSnapshot;
use nanocodex_durability::{ChildJournal, DurableAgentExt as _};
use nanocodex_subagents::{Registry, RegistryOwnership};

#[derive(Clone)]
pub(super) struct CliDurability {
    state: PortableDurableSession,
    root_session: String,
    registry: Option<std::sync::Weak<Registry>>,
}

impl CliDurability {
    pub(super) async fn open(
        persistence: LocalDurability,
        family: HarnessFamily,
        configured: Option<SessionId>,
        registry: Option<Arc<Registry>>,
    ) -> Result<Self> {
        if let Some(parent) = persistence.path.parent() {
            std::fs::create_dir_all(parent).wrap_err("failed to create durability directory")?;
        }
        let store = SqliteStore::open(&persistence.path)
            .wrap_err("failed to open local durability database")?;
        let state = PortableDurableSession::open(store, persistence.state_id)
            .await
            .wrap_err("failed to open local durability state")?;
        let root_session = if family == HarnessFamily::Codex {
            let mut identity = ChildJournal::open(
                state.child_store(),
                &format!("{}/native-root", state.state_id()),
            )
            .await?;
            match identity.load::<String>().await? {
                Some(saved) => {
                    let saved_id: SessionId = saved.parse()?;
                    if configured.is_some_and(|id| id != saved_id) {
                        return Err(eyre!(
                            "configured native identity differs from durable root"
                        ));
                    }
                    saved
                }
                None => {
                    let session = configured.unwrap_or_default().to_string();
                    identity.commit(&session).await?;
                    session
                }
            }
        } else {
            state.state_id().to_owned()
        };
        if let Some(registry) = &registry {
            registry
                .enable_durability(state.child_store(), &root_session)
                .await?;
        }
        Ok(Self {
            state,
            root_session,
            registry: registry.as_ref().map(Arc::downgrade),
        })
    }

    pub(super) fn codex_session_id(&self) -> Result<SessionId> {
        self.root_session.parse().map_err(Into::into)
    }

    pub(super) async fn codex_root<F>(
        &self,
        builder: nanocodex::NanocodexBuilder<F>,
    ) -> nanocodex::agent::Result<nanocodex::NanocodexBuilder<F>> {
        let mut builder = builder.session_id(self.root_session.parse().map_err(error)?);
        if let Some(registry) = &self.registry {
            let registry = registry
                .upgrade()
                .ok_or(nanocodex::NanocodexError::AgentStopped)?;
            builder = builder.turn_ownership(Arc::new(RegistryOwnership(registry)));
        }
        builder.durability(self.state.clone()).await
    }

    pub(super) async fn claude_root(
        &self,
        builder: nanocodex::claude::ClaudeBuilder,
    ) -> nanocodex::agent::Result<nanocodex::claude::ClaudeBuilder> {
        let mut builder = builder.session_id(self.root_session.clone());
        if let Some(registry) = &self.registry {
            let registry = registry
                .upgrade()
                .ok_or(nanocodex::NanocodexError::AgentStopped)?;
            builder = builder.turn_ownership(Arc::new(RegistryOwnership(registry)));
        }
        builder.durability(self.state.clone()).await
    }

    pub(super) async fn codex_child<F>(
        &self,
        mut builder: nanocodex::NanocodexBuilder<F>,
        snapshot: Option<ChildSnapshot>,
    ) -> nanocodex::agent::Result<nanocodex::NanocodexBuilder<F>> {
        let session = match &snapshot {
            Some(ChildSnapshot::Codex(snapshot)) => snapshot.session_id.parse().map_err(error)?,
            Some(_) => {
                return Err(nanocodex::NanocodexError::InvalidSessionSnapshot(
                    "expected Codex child checkpoint".into(),
                ));
            }
            None => SessionId::new(),
        };
        if let Some(snapshot) = snapshot {
            builder = builder.restore_runtime(snapshot)?;
        }
        builder = builder.session_id(session);
        if let Some(registry) = &self.registry {
            let registry = registry
                .upgrade()
                .ok_or(nanocodex::NanocodexError::AgentStopped)?;
            builder = builder.turn_ownership(Arc::new(RegistryOwnership::child(&registry)));
        }
        let state = PortableDurableSession::open(
            self.state.child_store(),
            format!("{}/child/{session}", self.state.state_id()),
        )
        .await
        .map_err(error)?;
        builder.durability(state).await
    }

    pub(super) async fn claude_child(
        &self,
        mut builder: nanocodex::claude::ClaudeBuilder,
        snapshot: Option<ChildSnapshot>,
    ) -> nanocodex::agent::Result<nanocodex::claude::ClaudeBuilder> {
        let session = match &snapshot {
            Some(ChildSnapshot::Native { session_id, .. }) => session_id.clone(),
            Some(_) => {
                return Err(nanocodex::NanocodexError::InvalidSessionSnapshot(
                    "expected Claude child checkpoint".into(),
                ));
            }
            None => format!("{}/child/{}", self.state.state_id(), SessionId::new()),
        };
        if let Some(snapshot) = snapshot {
            builder = builder.restore_runtime(snapshot)?;
        }
        builder = builder.session_id(session.clone());
        if let Some(registry) = &self.registry {
            let registry = registry
                .upgrade()
                .ok_or(nanocodex::NanocodexError::AgentStopped)?;
            builder = builder.turn_ownership(Arc::new(RegistryOwnership::child(&registry)));
        }
        let state = PortableDurableSession::open(self.state.child_store(), session)
            .await
            .map_err(error)?;
        builder.durability(state).await
    }
}

fn error(error: impl std::fmt::Display) -> nanocodex::NanocodexError {
    nanocodex::NanocodexError::InvalidExecutionPolicy(error.to_string())
}
