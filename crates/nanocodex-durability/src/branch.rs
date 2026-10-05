//! Explicit host-owned branching of settled historical checkpoints.
use crate::state::RetainedCheckpoint;
use crate::{
    DurableState, EncodedPayload, Error, OperationStatus, OwnerId, OwnerToken, Result, StateStore,
    Transition,
};
use serde::Serialize;

/// A retained operation eligible for an explicit history selection.
#[derive(Clone, Debug, Serialize)]
pub struct BranchTurn {
    /// Original durable operation identity.
    pub id: String,
    /// Original host input. This is reference data, never executable work.
    pub input: serde_json::Value,
}

/// An owned, settled source journal for a recoverable branch.
///
/// Acquiring this handle fences older owners through the normal store contract.
/// Pending operations are rejected; branching never imports execution receipts
/// as pending work or calls an external handler. The original journal remains
/// intact. Callers must separately validate provider-native checkpoint semantics.
pub struct CheckpointBranch<S> {
    store: S,
    source_id: String,
    branch_id: String,
    owner: OwnerToken,
    revision: u64,
    payload: String,
    state: DurableState,
}
impl<S: StateStore> CheckpointBranch<S> {
    /// Acquires the source with the standard owner protocol and rejects pending work.
    pub async fn open(mut store: S, source_id: impl Into<String>) -> Result<Self> {
        let source_id = source_id.into();
        let acquired = store.acquire(&source_id, OwnerId::new()).await?;
        let payload = acquired
            .state
            .payload
            .ok_or_else(|| Error::InvalidState("source journal does not exist".into()))?;
        let retained: RetainedCheckpoint =
            serde_json::from_str(&payload).map_err(Error::InvalidPayload)?;
        let state = DurableState::from_checkpoint(
            acquired.state.revision,
            retained.nanocodex_durable_state,
        )?;
        if !state.pending_operations().is_empty() {
            return Err(Error::InvalidState(
                "conversation rewind refuses pending operations; settle or reconcile them first"
                    .into(),
            ));
        }
        Ok(Self {
            store,
            source_id,
            branch_id: uuid::Uuid::new_v4().to_string(),
            owner: acquired.owner,
            revision: acquired.state.revision,
            payload,
            state,
        })
    }

    /// Reserved fresh UUID, so the host can prepare required policy and routing
    /// state before making the conversation journal resumable. No target state
    /// is created until publication.
    pub fn branch_id(&self) -> &str {
        &self.branch_id
    }

    /// Lists retained operations in submission order, with resolved original input.
    /// Old operations removed by the caller's retention policy cannot be selected.
    pub async fn turns(&mut self) -> Result<Vec<BranchTurn>> {
        let mut operations: Vec<_> = self.state.operations().iter().collect();
        operations.sort_by_key(|(_, op)| op.accepted_order);
        let mut turns = Vec::new();
        for (id, op) in operations {
            let input = op
                .input
                .load(&mut self.store, &self.source_id)
                .await?
                .decode()?;
            turns.push(BranchTurn {
                id: id.clone(),
                input,
            });
        }
        Ok(turns)
    }

    /// Loads the latest provider checkpoint without recovering or executing any work.
    pub async fn latest(&mut self) -> Result<EncodedPayload> {
        self.state
            .latest_checkpoint()
            .ok_or_else(|| Error::InvalidState("source has no checkpoint".into()))?
            .load(&mut self.store, &self.source_id)
            .await
    }

    /// Loads the terminal checkpoint immediately before the selected user operation.
    /// `None` means the selected operation was the first operation of a fresh journal.
    /// Missing retained history and unsafe cancelled boundaries fail closed.
    pub async fn before(&mut self, turn_id: &str) -> Result<Option<EncodedPayload>> {
        let selected = self
            .state
            .operation(turn_id)
            .ok_or_else(|| Error::InvalidState("unknown or expired rewind turn".into()))?;
        let prior = self
            .state
            .operations()
            .values()
            .filter(|op| op.accepted_order < selected.accepted_order)
            .max_by_key(|op| op.accepted_order);
        let checkpoint = match prior.map(|op| &op.status) {
            Some(OperationStatus::Completed { checkpoint, .. } | OperationStatus::Failed { checkpoint, .. } | OperationStatus::Cancelled { checkpoint: Some(checkpoint) }) => checkpoint,
            None if selected.accepted_order == 1 => return Ok(None),
            _ => return Err(Error::InvalidState("checkpoint before selected turn is unavailable; retained history cannot safely be reconstructed".into())),
        };
        Ok(Some(
            checkpoint.load(&mut self.store, &self.source_id).await?,
        ))
    }

    /// Checks owner/revision atomically through the store. No provider content changes.
    /// Use before any associated host mutation; a conflicting owner fails closed.
    pub async fn verify_source(&mut self) -> Result<()> {
        self.revision = self
            .store
            .replace(
                &self.source_id,
                &self.owner,
                self.revision,
                &self.payload,
                &[],
            )
            .await?;
        Ok(())
    }

    /// Publishes a new UUID journal containing only a checkpoint, with no pending work.
    /// The caller must not automatically retry an uncertain store error.
    pub async fn publish<T: Serialize + ?Sized>(mut self, checkpoint: &T) -> Result<String> {
        self.verify_source().await?;
        let id = self.branch_id.clone();
        let target = self.store.acquire(&id, OwnerId::new()).await?;
        if target.state.revision != 0 || target.state.payload.is_some() {
            return Err(Error::InvalidState(
                "new branch identity unexpectedly exists".into(),
            ));
        }
        let mut state = DurableState::default();
        state.apply_transition(
            1,
            Transition::CheckpointCommitted {
                checkpoint: EncodedPayload::encode(checkpoint)?,
            },
        )?;
        let records = state.stage_records();
        self.store
            .replace(
                &id,
                &target.owner,
                0,
                &state.checkpoint_payload()?,
                &records,
            )
            .await?;
        Ok(id)
    }
}
