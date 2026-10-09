//! Durable subagent task-tree journal beside a root's execution state.
//!
//! Every durability adapter attaches this to its builder, so any harness whose
//! root is durable also exposes a durable task tree on its handle, and a
//! subagent registry needs no host-specific wiring.

use std::{collections::HashSet, sync::Arc};

use nanocodex_agent::backend::{BackendFuture, ChildJournal, ChildJournalStore};

use crate::{
    StateStore,
    shared_store::SharedStore,
    state::EncodedPayload,
    store::{OwnerId, OwnerToken},
};

struct Fenced {
    store: SharedStore,
    owner: Option<(OwnerToken, u64)>,
    /// Record keys this journal has committed. Records are immutable, so a
    /// growing checkpoint resends only its new chunks.
    committed: HashSet<String>,
}

struct Inner {
    state_id: String,
    fenced: tokio::sync::Mutex<Fenced>,
}

impl Inner {
    async fn acquire(&self, fenced: &mut Fenced) -> std::io::Result<Option<String>> {
        let owned = fenced
            .store
            .acquire(&self.state_id, OwnerId::new())
            .await
            .map_err(std::io::Error::other)?;
        fenced.owner = Some((owned.owner, owned.state.revision));
        Ok(owned.state.payload)
    }
}

#[derive(Clone)]
struct SharedJournal(Arc<Inner>);

impl ChildJournalStore for SharedJournal {
    fn load(&self) -> BackendFuture<std::io::Result<Option<String>>> {
        let inner = Arc::clone(&self.0);
        Box::pin(async move {
            let mut fenced = inner.fenced.lock().await;
            inner.acquire(&mut fenced).await
        })
    }

    fn save(&self, payload: String, records: Vec<Arc<str>>) -> BackendFuture<std::io::Result<()>> {
        let inner = Arc::clone(&self.0);
        Box::pin(async move {
            // Checkpoints are content-addressed and chunked like execution
            // payloads: the journal row stays small, and an unchanged prefix of
            // a growing child conversation reuses its stored chunks.
            let mut staged = Vec::new();
            for json in records {
                EncodedPayload::from_json(json).stage(&mut staged);
            }
            staged.sort_unstable_by(|a, b| a.key.cmp(&b.key));
            staged.dedup_by(|a, b| a.key == b.key);
            let mut fenced = inner.fenced.lock().await;
            staged.retain(|record| !fenced.committed.contains(&record.key));
            if fenced.owner.is_none() {
                inner.acquire(&mut fenced).await?;
            }
            let (owner, revision) = fenced
                .owner
                .clone()
                .ok_or_else(|| std::io::Error::other("journal owner was not acquired"))?;
            // A newer runtime acquiring this journal fences this writer.
            let revision = fenced
                .store
                .replace(&inner.state_id, &owner, revision, &payload, &staged)
                .await
                .map_err(std::io::Error::other)?;
            fenced.owner = Some((owner, revision));
            fenced
                .committed
                .extend(staged.into_iter().map(|record| record.key));
            Ok(())
        })
    }

    fn load_record(&self, key: String) -> BackendFuture<std::io::Result<String>> {
        let inner = Arc::clone(&self.0);
        Box::pin(async move {
            let mut fenced = inner.fenced.lock().await;
            let loaded = EncodedPayload::by_key(&key)
                .load(&mut fenced.store, &inner.state_id)
                .await
                .map_err(std::io::Error::other)?;
            Ok(loaded.json().map_err(std::io::Error::other)?.to_owned())
        })
    }
}

/// The task-tree journal of the durable root stored as `root_state_id`.
pub(crate) fn child_journal(store: SharedStore, root_state_id: &str) -> ChildJournal {
    ChildJournal::new(Arc::new(SharedJournal(Arc::new(Inner {
        state_id: format!("{root_state_id}:subagents"),
        fenced: tokio::sync::Mutex::new(Fenced {
            store,
            owner: None,
            committed: HashSet::new(),
        }),
    }))))
}
