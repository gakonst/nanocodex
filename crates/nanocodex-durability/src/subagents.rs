//! Durable subagent task-tree journal beside a durable root's state.

use nanocodex_agent::{SubagentStore, SubagentStoreFuture};

use crate::{OwnerId, OwnerToken, StateStore as _, StoreError, shared_store::SharedStore};

/// Journals one root's subagent tree under a separate fenced state ID
/// (`{root_state_id}:subagents`) in the root's own store.
///
/// The key depends only on the durable state: a reopened root may receive a
/// new runtime session ID, but it always owns the same tree. Journals written
/// under the earlier `{root_state_id}:subagents:{root_session_id}` key are
/// still loaded when no state-keyed journal exists.
///
/// Each journal value has its own owner fence. A newer process that restores
/// the tree fences an older writer, which then stops journaling instead of
/// overwriting the newer tree.
pub(crate) struct SubagentJournal {
    store: SharedStore,
    state_id: String,
    legacy_prefix: String,
    owner: tokio::sync::Mutex<JournalOwner>,
}

#[derive(Default)]
struct JournalOwner {
    current: Option<(OwnerToken, u64)>,
    fenced: bool,
}

impl SubagentJournal {
    pub(crate) fn new(store: SharedStore, root_state_id: &str) -> Self {
        Self {
            store,
            state_id: format!("{root_state_id}:subagents"),
            legacy_prefix: format!("{root_state_id}:subagents:"),
            owner: tokio::sync::Mutex::new(JournalOwner::default()),
        }
    }

    async fn acquire(
        &self,
        state_id: &str,
    ) -> Result<(OwnerToken, u64, Option<String>), StoreError> {
        let mut store = self.store.clone();
        let owned = store.acquire(state_id, OwnerId::new()).await?;
        Ok((owned.owner, owned.state.revision, owned.state.payload))
    }
}

impl SubagentStore for SubagentJournal {
    fn load<'a>(
        &'a self,
        root_session_id: &'a str,
    ) -> SubagentStoreFuture<'a, std::io::Result<Option<String>>> {
        Box::pin(async move {
            let mut owner = self.owner.lock().await;
            let (token, revision, payload) = self
                .acquire(&self.state_id)
                .await
                .map_err(std::io::Error::other)?;
            owner.current = Some((token, revision));
            owner.fenced = false;
            if payload.is_some() {
                return Ok(payload);
            }
            let mut store = self.store.clone();
            let legacy = store
                .acquire(
                    &format!("{}{root_session_id}", self.legacy_prefix),
                    OwnerId::new(),
                )
                .await
                .map_err(std::io::Error::other)?;
            Ok(legacy.state.payload)
        })
    }

    fn save<'a>(
        &'a self,
        _root_session_id: &'a str,
        payload: String,
    ) -> SubagentStoreFuture<'a, std::io::Result<()>> {
        Box::pin(async move {
            let mut owner = self.owner.lock().await;
            if owner.fenced {
                return Err(std::io::Error::other(StoreError::Fenced));
            }
            let mut store = self.store.clone();
            for attempt in 0..2 {
                let (token, revision) = match &owner.current {
                    Some((token, revision)) => (token.clone(), *revision),
                    None => {
                        let (token, revision, _) = self
                            .acquire(&self.state_id)
                            .await
                            .map_err(std::io::Error::other)?;
                        (token, revision)
                    }
                };
                match store
                    .replace(&self.state_id, &token, revision, &payload, &[])
                    .await
                {
                    Ok(revision) => {
                        owner.current = Some((token, revision));
                        return Ok(());
                    }
                    Err(StoreError::Fenced) => {
                        owner.current = None;
                        owner.fenced = true;
                        return Err(std::io::Error::other(StoreError::Fenced));
                    }
                    Err(error) if attempt == 1 => {
                        owner.current = None;
                        return Err(std::io::Error::other(error));
                    }
                    // An uncertain or stale write is safe to repeat: each save
                    // replaces the complete value under a freshly acquired fence.
                    Err(_) => owner.current = None,
                }
            }
            unreachable!("journal save returns within two attempts")
        })
    }
}

impl crate::DurableSession {
    /// Journal for subagent trees rooted at this durable state, stored in the
    /// same host store under a distinct fenced state ID.
    pub(crate) fn subagent_journal(&self) -> std::sync::Arc<dyn SubagentStore> {
        std::sync::Arc::new(SubagentJournal::new(self.shared_store(), self.state_id()))
    }
}
