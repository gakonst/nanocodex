use std::{collections::BTreeMap, sync::Arc};

use crate::{Error, Result};
use serde::{Serialize, de::DeserializeOwned};

const STATE_FORMAT: u8 = 4;
const RECORD_BYTES: usize = 256_000;

/// An immutable payload reference. Content is loaded only for its consumer.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
#[serde(transparent)]
pub struct EncodedPayload {
    pub(crate) key: Arc<str>,
    #[serde(skip)]
    content: Option<Arc<str>>,
    #[serde(skip)]
    pending: Vec<crate::StoreRecord>,
}

impl EncodedPayload {
    pub(crate) fn encode<T: Serialize + ?Sized>(value: &T) -> Result<Self> {
        let json = serde_json::to_string(value).map_err(Error::InvalidPayload)?;
        Ok(Self {
            key: record_key(&json).into(),
            content: Some(json.into()),
            pending: Vec::new(),
        })
    }

    /// Decodes a payload loaded by its durable session.
    pub fn decode<T: DeserializeOwned>(&self) -> Result<T> {
        serde_json::from_str(self.json()?).map_err(Error::InvalidPayload)
    }

    /// Returns content loaded by a session operation.
    pub fn json(&self) -> Result<&str> {
        self.content.as_deref().ok_or_else(|| {
            Error::InvalidState(
                "resolve the payload through its durable session before reading it".into(),
            )
        })
    }

    pub(crate) fn reference(&self) -> Self {
        Self {
            key: self.key.clone(),
            content: None,
            pending: Vec::new(),
        }
    }

    pub(crate) fn with_records(mut self, records: Vec<crate::StoreRecord>) -> Self {
        self.pending = records;
        self
    }

    pub(crate) fn stage(&mut self, records: &mut Vec<crate::StoreRecord>) {
        records.append(&mut self.pending);
        let Some(content) = self.content.take() else {
            return;
        };
        if content.len() < RECORD_BYTES {
            records.push(crate::StoreRecord {
                key: self.key.to_string(),
                value: format!("={content}"),
            });
            return;
        }
        let mut offset = 0;
        let mut count = 0;
        while offset < content.len() {
            let mut end = (offset + RECORD_BYTES).min(content.len());
            while !content.is_char_boundary(end) {
                end -= 1;
            }
            records.push(crate::StoreRecord {
                key: format!("{}/{count}", self.key),
                value: content[offset..end].to_owned(),
            });
            count += 1;
            offset = end;
        }
        records.push(crate::StoreRecord {
            key: self.key.to_string(),
            value: format!("+{count}"),
        });
    }

    pub(crate) async fn load(
        &self,
        store: &mut dyn crate::StateStore,
        state_id: &str,
    ) -> Result<Self> {
        if self.content.is_some() {
            return Ok(self.clone());
        }
        let record = store
            .read_record(state_id, &self.key)
            .await?
            .ok_or_else(|| Error::InvalidState(format!("missing payload record {}", self.key)))?;
        self.load_record(store, state_id, record).await
    }

    pub(crate) async fn load_many(
        values: &[Self],
        store: &mut dyn crate::StateStore,
        state_id: &str,
    ) -> Result<Vec<Self>> {
        let mut result = Vec::with_capacity(values.len());
        for page in values.chunks(16) {
            let keys: Vec<_> = page.iter().map(|value| value.key.to_string()).collect();
            let records = store.read_records(state_id, &keys).await?;
            if records.len() != page.len() {
                return Err(Error::InvalidState("record batch length mismatch".into()));
            }
            for (value, record) in page.iter().zip(records) {
                let record = record.ok_or_else(|| {
                    Error::InvalidState(format!("missing payload record {}", value.key))
                })?;
                result.push(value.load_record(store, state_id, record).await?);
            }
        }
        Ok(result)
    }

    async fn load_record(
        &self,
        store: &mut dyn crate::StateStore,
        state_id: &str,
        record: String,
    ) -> Result<Self> {
        let content = if let Some(content) = record.strip_prefix('=') {
            content.to_owned()
        } else {
            let count: usize = record
                .strip_prefix('+')
                .ok_or_else(|| Error::InvalidState("invalid payload record".into()))?
                .parse()
                .map_err(|_| Error::InvalidState("invalid payload record count".into()))?;
            let mut content = String::new();
            for index in 0..count {
                let chunk = store
                    .read_record(state_id, &format!("{}/{index}", self.key))
                    .await?
                    .ok_or_else(|| {
                        Error::InvalidState(format!("missing payload chunk {}/{index}", self.key))
                    })?;
                content.push_str(&chunk);
            }
            content
        };
        if record_key(&content) != self.key.as_ref() {
            return Err(Error::InvalidState(format!(
                "payload record checksum mismatch {}",
                self.key
            )));
        }
        Ok(Self {
            key: self.key.clone(),
            content: Some(content.into()),
            pending: Vec::new(),
        })
    }
}

fn record_key(value: &str) -> String {
    use sha2::{Digest, Sha256};
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut encoded = String::with_capacity(64);
    for byte in Sha256::digest(value.as_bytes()) {
        encoded.push(HEX[(byte >> 4) as usize] as char);
        encoded.push(HEX[(byte & 15) as usize] as char);
    }
    encoded
}

impl PartialEq for EncodedPayload {
    fn eq(&self, other: &Self) -> bool {
        self.key == other.key
    }
}
impl Eq for EncodedPayload {}

/// One Rust-owned durable state entry.
#[derive(Clone, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum Transition {
    /// Replaces the current execution position after all preceding effects settled.
    /// The conversation in this value subsumes those effects' recovery receipts.
    ExecutionAdvanced {
        /// Accepted operation identity.
        operation_id: String,
        /// Opaque current agent state, rather than a history of requests.
        continuation: EncodedPayload,
    },
    /// A host-visible operation was durably accepted.
    OperationAccepted {
        /// Caller-provided idempotency identity.
        operation_id: String,
        /// Opaque typed input encoded by the Rust consumer.
        input: EncodedPayload,
    },
    /// An external step began.
    StepStarted {
        /// Accepted operation identity.
        operation_id: String,
        /// Stable step identity within the operation.
        step_id: String,
        /// Semantic step kind used for diagnostics.
        kind: String,
        /// Opaque typed step input.
        input: EncodedPayload,
    },
    /// An external step completed with a replayable output.
    StepCompleted {
        /// Accepted operation identity.
        operation_id: String,
        /// Stable step identity within the operation.
        step_id: String,
        /// Opaque typed output returned during replay.
        output: EncodedPayload,
        /// Provider response ID extracted from a completed late-wake model step.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        late_response_id: Option<String>,
    },
    /// Live steering input was accepted for an active operation.
    SteerAccepted {
        /// Caller identity atomically retained with this acceptance.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        message_id: Option<String>,
        /// Accepted operation identity.
        operation_id: String,
        /// Stable one-based FIFO position within the operation.
        steer_index: u32,
        /// Model call that was current when the steering input was accepted.
        accepted_after_model_call_index: u32,
        /// Exact typed steering prompt.
        input: EncodedPayload,
    },
    /// The latest unbound steer was withdrawn before model consumption.
    SteerWithdrawn {
        /// Accepted operation identity.
        operation_id: String,
        /// One-based position of the latest accepted steer.
        steer_index: u32,
    },
    /// Accepted steering input was bound to its consuming model boundary.
    SteerBound {
        /// Accepted operation identity.
        operation_id: String,
        /// Stable one-based FIFO position within the operation.
        steer_index: u32,
        /// Model-call ordinal before which the steer is applied.
        model_call_index: u32,
    },
    /// A typed output accepted for delivery at a future model request boundary.
    BoundaryOutputAccepted {
        /// Admitted operation.
        operation_id: String,
        /// One-based queue position.
        output_index: u32,
        /// Current model request when accepted.
        accepted_after_model_call_index: u32,
        /// Caller idempotency identity.
        message_id: String,
        /// Typed serialized output.
        input: EncodedPayload,
    },
    /// A typed output is assigned to its consuming model request.
    BoundaryOutputBound {
        /// Admitted operation.
        operation_id: String,
        /// Original one-based acceptance position.
        output_index: u32,
        /// Request ordinal consuming this output.
        model_call_index: u32,
    },
    /// A completed model step confirms that a bound output reached a response boundary.
    BoundaryOutputConfirmed {
        /// Admitted operation.
        operation_id: String,
        /// Original one-based acceptance position.
        output_index: u32,
        /// Request ordinal consuming this output.
        model_call_index: u32,
        /// Actual provider response ID of the completed model step.
        response_id: String,
    },
    /// An operation completed and advanced the durable session checkpoint.
    OperationCompleted {
        /// Accepted operation identity.
        operation_id: String,
        /// Opaque resumable agent checkpoint.
        checkpoint: EncodedPayload,
        /// Opaque completed result returned to duplicate submissions.
        output: EncodedPayload,
    },
    /// An operation failed and advanced the durable session checkpoint.
    OperationFailed {
        /// Accepted operation identity.
        operation_id: String,
        /// Opaque resumable agent checkpoint.
        checkpoint: EncodedPayload,
        /// Stable terminal failure detail.
        error: String,
    },
    /// An operation was explicitly cancelled.
    OperationCancelled {
        /// Accepted operation identity.
        operation_id: String,
        /// Safe interrupted checkpoint for an active operation. A queued
        /// cancellation has no new model boundary.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        checkpoint: Option<EncodedPayload>,
    },
    /// A model-only boundary, such as explicit standalone compaction, advanced
    /// the resumable session without terminalizing an operation.
    CheckpointCommitted {
        /// Opaque resumable agent checkpoint.
        checkpoint: EncodedPayload,
    },
}

/// Reduced status of one operation.
#[derive(Clone, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum OperationStatus {
    /// Accepted work may be attempted or resumed.
    Pending,
    /// Work completed with an opaque result and checkpoint.
    Completed {
        /// Resumable checkpoint committed atomically with the result.
        checkpoint: EncodedPayload,
        /// Result returned to duplicate submissions.
        output: EncodedPayload,
    },
    /// Work failed with a resumable checkpoint and retained diagnostic.
    Failed {
        /// Resumable checkpoint committed atomically with the failure.
        checkpoint: EncodedPayload,
        /// Failure returned to duplicate submissions.
        error: String,
    },
    /// Work was explicitly cancelled, optionally after advancing the safe
    /// interrupted checkpoint.
    Cancelled {
        /// Safe checkpoint committed by active cancellation.
        checkpoint: Option<EncodedPayload>,
    },
}

impl OperationStatus {
    /// Returns whether this operation cannot execute again.
    #[must_use]
    pub const fn is_terminal(&self) -> bool {
        matches!(
            self,
            Self::Completed { .. } | Self::Failed { .. } | Self::Cancelled { .. }
        )
    }
}

/// Reduced status of one step.
#[derive(Clone, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum StepStatus {
    /// The step started but has no committed output yet.
    EffectPending,
    /// The external effect's exact output settled durably.
    Completed(EncodedPayload),
}

/// Reduced durable step state.
#[derive(Clone, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub struct StepState {
    /// Semantic kind recorded by the caller.
    pub kind: String,
    /// Original opaque step input.
    pub input: EncodedPayload,
    /// Current reduced status.
    pub status: StepStatus,
    /// Number of committed starts for this step.
    pub attempts: u32,
}

/// One live steering input retained for deterministic operation recovery.
#[derive(Clone, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub struct SteerState {
    /// Optional caller identity for pending withdrawal after recovery.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message_id: Option<String>,
    /// Exact typed steering prompt.
    pub input: EncodedPayload,
    /// Model call that was current when the steering input was accepted.
    pub accepted_after_model_call_index: u32,
    /// Model-call ordinal before which the steer is applied, once known.
    pub model_call_index: Option<u32>,
}

/// A small durable caller receipt retained after consumption or withdrawal.
#[derive(Clone, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub struct IdentifiedSteerReceipt {
    /// Fingerprint of the exact serialized prompt.
    pub input_key: String,
    /// Original acceptance ordinal.
    pub index: u32,
    /// Withdrawn identities cannot be accepted again.
    pub withdrawn: bool,
}

/// A typed output waiting for a future model request (never a completed delivery).
#[derive(Clone, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub struct BoundaryOutputState {
    /// Caller idempotency identity.
    pub message_id: String,
    /// Encoded typed output.
    pub input: EncodedPayload,
    /// Current model request when accepted.
    pub accepted_after_model_call_index: u32,
    /// Request ordinal selected for delivery, or none before binding.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_call_index: Option<u32>,
}

/// Caller receipt independent of steering receipts.
#[derive(Clone, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub struct BoundaryOutputReceipt {
    /// Content fingerprint for exact duplicate detection.
    pub input_key: String,
    /// Exact original terminal function-call ID, if the accepted payload was typed.
    /// Missing on legacy/generic receipts; a call-validated lookup then fails closed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub call_id: Option<String>,
    /// Original one-based queue position.
    pub index: u32,
    /// Assignment alone does not prove model uptake.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bound_model_call_index: Option<u32>,
    /// A durable completed model step confirms this response boundary.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confirmed_model_call_index: Option<u32>,
    /// Provider response ID corresponding to the confirmed model call.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response_id: Option<String>,
    /// Terminal failure/cancellation discarded the live output without model uptake.
    #[serde(default)]
    pub discarded: bool,
}

/// Reduced durable operation state.
#[derive(Clone, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub struct OperationState {
    /// Caller receipts survive retirement of live steering bodies.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub steer_receipts: BTreeMap<String, IdentifiedSteerReceipt>,
    /// Independent idempotency namespace for typed boundary outputs.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub boundary_output_receipts: BTreeMap<String, BoundaryOutputReceipt>,
    /// Accepted but not yet delivered typed outputs.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub boundary_outputs: Vec<BoundaryOutputState>,
    /// Number of confirmed FIFO bodies removed after a durable advance.
    #[serde(default)]
    pub retired_boundary_outputs: u32,
    /// First completed provider model step of an idle wake; retained after step retirement.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub late_model_response: Option<(u32, String)>,
    /// Current conversation and execution position; settled batches are retired atomically.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub continuation: Option<EncodedPayload>,
    /// Model batches already incorporated in the current conversation.
    #[serde(default)]
    pub retired_model_calls: u32,
    /// Steering inputs already incorporated in the current conversation.
    pub retired_steers: u32,
    /// Original opaque operation input.
    pub input: EncodedPayload,
    /// Current operation status.
    pub status: OperationStatus,
    /// Ordered durable steps by identity.
    pub steps: BTreeMap<String, StepState>,
    /// Live steering inputs in their accepted FIFO order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub steers: Vec<SteerState>,
    pub(crate) accepted_order: u64,
}

impl OperationState {
    pub(crate) fn cancellation_requires_checkpoint(&self) -> bool {
        self.continuation.is_some()
            || self.retired_model_calls != 0
            || !self.steps.is_empty()
            || !self.steers.is_empty()
            || !self.boundary_outputs.is_empty()
    }

    fn retire_steps(&mut self) {
        for (id, step) in &self.steps {
            if step.kind == "model_call"
                && matches!(step.status, StepStatus::Completed(_))
                && let Some(index) = id
                    .strip_prefix("model-")
                    .and_then(|id| id.parse::<u32>().ok())
            {
                self.retired_model_calls = self.retired_model_calls.max(index);
            }
        }
        self.steps.clear();
        let consumed = self
            .steers
            .iter()
            .take_while(|steer| {
                steer
                    .model_call_index
                    .is_some_and(|index| index <= self.retired_model_calls)
            })
            .count();
        // Accepted indexes already fit u32; retirement preserves that total.
        self.retired_steers += consumed as u32;
        self.steers.drain(..consumed);
        let consumed = self
            .boundary_outputs
            .iter()
            .take_while(|output| {
                self.boundary_output_receipts
                    .get(&output.message_id)
                    .is_some_and(|receipt| {
                        receipt
                            .confirmed_model_call_index
                            .is_some_and(|index| index <= self.retired_model_calls)
                    })
            })
            .count();
        self.retired_boundary_outputs += consumed as u32;
        self.boundary_outputs.drain(..consumed);
    }
}

/// Complete state reduced from an complete retained state.
#[derive(Clone, Debug, Default)]
pub struct DurableState {
    revision: u64,
    operations: BTreeMap<String, OperationState>,
    latest_checkpoint: Option<(u64, EncodedPayload)>,
}

#[derive(serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct DurableCheckpoint {
    format: u8,
    operations: BTreeMap<String, OperationState>,
    latest_checkpoint: Option<EncodedPayload>,
}

#[derive(serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RetainedCheckpoint {
    pub(crate) nanocodex_durable_state: DurableCheckpoint,
}

#[derive(serde::Serialize)]
struct DurableCheckpointRef<'a> {
    format: u8,
    operations: &'a BTreeMap<String, OperationState>,
    latest_checkpoint: Option<&'a EncodedPayload>,
}

#[derive(serde::Serialize)]
struct RetainedCheckpointRef<'a> {
    nanocodex_durable_state: DurableCheckpointRef<'a>,
}

impl DurableState {
    pub(crate) fn stage_records(&mut self) -> Vec<crate::StoreRecord> {
        let mut records = Vec::new();
        for operation in self.operations.values_mut() {
            operation.input.stage(&mut records);
            if let Some(value) = &mut operation.continuation {
                value.stage(&mut records);
            }
            match &mut operation.status {
                OperationStatus::Completed { checkpoint, output } => {
                    checkpoint.stage(&mut records);
                    output.stage(&mut records);
                }
                OperationStatus::Failed { checkpoint, .. } => checkpoint.stage(&mut records),
                OperationStatus::Cancelled {
                    checkpoint: Some(value),
                } => value.stage(&mut records),
                _ => {}
            }
            for step in operation.steps.values_mut() {
                step.input.stage(&mut records);
                if let StepStatus::Completed(output) = &mut step.status {
                    output.stage(&mut records);
                }
            }
            for steer in &mut operation.steers {
                steer.input.stage(&mut records);
            }
            for output in &mut operation.boundary_outputs {
                output.input.stage(&mut records);
            }
        }
        if let Some((_, value)) = &mut self.latest_checkpoint {
            value.stage(&mut records);
        }
        records.sort_unstable_by(|a, b| a.key.cmp(&b.key));
        records.dedup_by(|a, b| a.key == b.key);
        records
    }

    /// Current optimistic store revision.
    #[must_use]
    pub const fn revision(&self) -> u64 {
        self.revision
    }

    /// Operations keyed by caller-provided identity.
    #[must_use]
    pub const fn operations(&self) -> &BTreeMap<String, OperationState> {
        &self.operations
    }

    /// Looks up one operation.
    #[must_use]
    pub fn operation(&self, operation_id: &str) -> Option<&OperationState> {
        self.operations.get(operation_id)
    }

    /// Returns accepted non-terminal operations in submission order.
    #[must_use]
    pub fn pending_operations(&self) -> Vec<(&str, &OperationState)> {
        let mut operations = self
            .operations
            .iter()
            .filter(|(_, operation)| !operation.status.is_terminal())
            .map(|(id, operation)| (id.as_str(), operation))
            .collect::<Vec<_>>();
        operations.sort_by_key(|(_, operation)| operation.accepted_order);
        operations
    }

    pub(crate) fn first_pending_operation(&self) -> Option<(&str, &OperationState)> {
        self.first_pending_operation_where(|_| true)
    }

    pub(crate) fn first_pending_operation_where(
        &self,
        mut predicate: impl FnMut(&str) -> bool,
    ) -> Option<(&str, &OperationState)> {
        self.operations
            .iter()
            .filter(|(id, operation)| !operation.status.is_terminal() && predicate(id.as_str()))
            .min_by_key(|(_, operation)| operation.accepted_order)
            .map(|(id, operation)| (id.as_str(), operation))
    }

    /// Returns the latest terminal checkpoint in operation order.
    #[must_use]
    pub fn latest_checkpoint(&self) -> Option<&EncodedPayload> {
        self.latest_checkpoint
            .as_ref()
            .map(|(_, checkpoint)| checkpoint)
    }

    pub(crate) fn checkpoint_payload(&self) -> Result<String> {
        serde_json::to_string(&RetainedCheckpointRef {
            nanocodex_durable_state: DurableCheckpointRef {
                format: STATE_FORMAT,
                operations: &self.operations,
                latest_checkpoint: self.latest_checkpoint(),
            },
        })
        .map_err(Error::InvalidPayload)
    }

    pub(crate) fn retain_terminal_receipts(&mut self, limit: usize) -> bool {
        let before = self.operations.len();
        Self::retain_terminal_operations(&mut self.operations, limit);
        let mut changed = self.operations.len() != before;
        for operation in self
            .operations
            .values_mut()
            .filter(|operation| operation.status.is_terminal())
        {
            // Terminal replay uses only input, result, and checkpoint. Keeping
            // every intermediate full-history model request multiplies memory
            // and write volume across long conversations.
            changed |= !operation.steps.is_empty()
                || !operation.steers.is_empty()
                || !operation.boundary_outputs.is_empty();
            operation.steps.clear();
            changed |= operation.continuation.take().is_some();
            operation.steers.clear();
            operation.boundary_outputs.clear();
        }
        changed
    }

    fn retain_terminal_operations(operations: &mut BTreeMap<String, OperationState>, limit: usize) {
        let mut terminal_orders = operations
            .iter()
            .filter(|(id, operation)| {
                !id.starts_with("late-output:") && operation.status.is_terminal()
            })
            .map(|(_, operation)| operation.accepted_order)
            .collect::<Vec<_>>();
        terminal_orders.sort_unstable_by(|left, right| right.cmp(left));
        terminal_orders.truncate(limit);
        let retained = terminal_orders
            .into_iter()
            .collect::<std::collections::BTreeSet<_>>();
        operations.retain(|operation_id, operation| {
            // A late-output ID is the original caller's identity, not a
            // generated turn ID. Its terminal receipt is both the exact-input
            // deduplication ledger and the recovery checkpoint for a fenced
            // cohort. Pruning it would permit the same ID (even with a
            // different body) to execute again after transcript compaction,
            // or strand a cohort between two member commits. Retain these
            // receipts independently of the ordinary bounded turn policy.
            operation_id.starts_with("late-output:")
                || !operation.status.is_terminal()
                || retained.contains(&operation.accepted_order)
        });
    }

    pub(crate) fn from_checkpoint(revision: u64, checkpoint: DurableCheckpoint) -> Result<Self> {
        if revision == 0 {
            return Err(Error::InvalidState(
                "a compacted state checkpoint must have a positive revision".to_owned(),
            ));
        }
        if checkpoint.format != STATE_FORMAT {
            return Err(Error::InvalidState(format!(
                "unsupported state format {}",
                checkpoint.format
            )));
        }
        let mut accepted_orders = std::collections::BTreeSet::new();
        for (operation_id, operation) in &checkpoint.operations {
            ensure_nonempty(operation_id, "operation ID")?;
            if operation.accepted_order == 0
                || operation.accepted_order > revision
                || !accepted_orders.insert(operation.accepted_order)
            {
                return Err(Error::InvalidState(format!(
                    "operation `{operation_id}` has an invalid compacted acceptance order"
                )));
            }
            if operation.status.is_terminal() && operation.continuation.is_some() {
                return Err(Error::InvalidState(
                    "terminal operation retained active execution state".into(),
                ));
            }
            if !operation.status.is_terminal()
                && operation.retired_model_calls != 0
                && operation.continuation.is_none()
            {
                return Err(Error::InvalidState(
                    "retired model batches have no current conversation".into(),
                ));
            }
            for (step_id, step) in &operation.steps {
                ensure_nonempty(step_id, "step ID")?;
                ensure_nonempty(&step.kind, "step kind")?;
                if step.attempts == 0 {
                    return Err(Error::InvalidState(format!(
                        "step `{step_id}` in operation `{operation_id}` has no committed start"
                    )));
                }
            }
            if let Some((index, response_id)) = &operation.late_model_response {
                let step_id = format!("model-{index}");
                if !operation_id.starts_with("late-continuation:")
                    || *index == 0
                    || response_id.is_empty()
                    || (*index > operation.retired_model_calls
                        && !operation.steps.get(&step_id).is_some_and(|step| {
                            step.kind == "model_call"
                                && matches!(step.status, StepStatus::Completed(_))
                        }))
                {
                    return Err(Error::InvalidState(
                        "invalid retained late wake model receipt".into(),
                    ));
                }
            }
            let mut previous_model_call_index = None;
            let mut saw_unbound_steer = false;
            for (offset, steer) in operation.steers.iter().enumerate() {
                if steer.accepted_after_model_call_index == 0
                    || steer.model_call_index.is_some_and(|model_call_index| {
                        model_call_index <= steer.accepted_after_model_call_index
                    })
                {
                    return Err(Error::InvalidState(format!(
                        "steer {} in operation `{operation_id}` has an invalid model boundary",
                        offset + 1 + operation.retired_steers as usize
                    )));
                }
                match steer.model_call_index {
                    Some(current) => {
                        if saw_unbound_steer {
                            return Err(Error::InvalidState(format!(
                                "steer {} in operation `{operation_id}` was bound after an unbound steer",
                                offset + 1 + operation.retired_steers as usize
                            )));
                        }
                        if previous_model_call_index.is_some_and(|previous| current < previous) {
                            return Err(Error::InvalidState(format!(
                                "steer {} in operation `{operation_id}` moved before an earlier steer",
                                offset + 1 + operation.retired_steers as usize
                            )));
                        }
                        previous_model_call_index = Some(current);
                    }
                    None => saw_unbound_steer = true,
                }
            }
            let mut previous_output_boundary = None;
            let mut saw_unbound_output = false;
            for (offset, output) in operation.boundary_outputs.iter().enumerate() {
                if output.message_id.is_empty()
                    || operation
                        .boundary_output_receipts
                        .get(&output.message_id)
                        .is_none_or(|receipt| {
                            receipt.index as usize
                                != offset + 1 + operation.retired_boundary_outputs as usize
                                || receipt.input_key != output.input.key.as_ref()
                        })
                {
                    return Err(Error::InvalidState(format!(
                        "invalid boundary output in operation `{operation_id}`"
                    )));
                }
                let receipt = &operation.boundary_output_receipts[&output.message_id];
                if receipt.bound_model_call_index != output.model_call_index
                    || receipt
                        .confirmed_model_call_index
                        .is_some_and(|index| Some(index) != output.model_call_index)
                    || receipt.response_id.is_some() != receipt.confirmed_model_call_index.is_some()
                {
                    return Err(Error::InvalidState(
                        "boundary output receipt disagrees with live output".into(),
                    ));
                }
                match output.model_call_index {
                    Some(current)
                        if current <= output.accepted_after_model_call_index
                            || saw_unbound_output
                            || previous_output_boundary
                                .is_some_and(|previous| current < previous) =>
                    {
                        return Err(Error::InvalidState(
                            "invalid boundary output binding".into(),
                        ));
                    }
                    Some(current) => previous_output_boundary = Some(current),
                    None => saw_unbound_output = true,
                }
            }
            if matches!(operation.status, OperationStatus::Completed { .. }) {
                ensure_completed_steers_consumed(operation_id, operation)?;
                ensure_completed_boundary_outputs_consumed(operation_id, operation)?;
            }
            if matches!(
                &operation.status,
                OperationStatus::Cancelled { checkpoint: None }
            ) && operation.cancellation_requires_checkpoint()
            {
                return Err(Error::InvalidState(format!(
                    "started operation `{operation_id}` was cancelled without a checkpoint"
                )));
            }
        }
        // Live transitions share the latest checkpoint with their terminal
        // receipt. Deserialization loses that Arc sharing; restore it before
        // constructing the agent so a cold reopen does not retain a second
        // full conversation. Standalone checkpoints can differ and stay intact.
        let latest_checkpoint = checkpoint.latest_checkpoint.map(|latest| {
            let shared = checkpoint.operations.values().rev().find_map(|operation| {
                let candidate = match &operation.status {
                    OperationStatus::Completed { checkpoint, .. }
                    | OperationStatus::Failed { checkpoint, .. }
                    | OperationStatus::Cancelled {
                        checkpoint: Some(checkpoint),
                    } => checkpoint,
                    _ => return None,
                };
                (candidate == &latest).then(|| candidate.clone())
            });
            (revision, shared.unwrap_or(latest))
        });
        let state = Self {
            revision,
            operations: checkpoint.operations,
            latest_checkpoint,
        };
        for (operation_id, operation) in &state.operations {
            if matches!(
                &operation.status,
                OperationStatus::Completed { .. }
                    | OperationStatus::Failed { .. }
                    | OperationStatus::Cancelled {
                        checkpoint: Some(_)
                    }
            ) {
                state.ensure_prior_operations_terminal(operation_id)?;
            }
        }
        Ok(state)
    }

    pub(crate) fn validate_transition(&self, revision: u64, entry: &Transition) -> Result<()> {
        let expected_revision = self.revision.checked_add(1).ok_or_else(|| {
            Error::InvalidState("state revision exceeded the u64 range".to_owned())
        })?;
        if revision != expected_revision {
            return Err(Error::InvalidState(format!(
                "expected revision {}, found {revision}",
                expected_revision
            )));
        }
        self.validate(entry)
    }

    pub(crate) fn apply_transition(&mut self, revision: u64, entry: Transition) -> Result<()> {
        self.validate_transition(revision, &entry)?;
        self.apply(revision, entry)?;
        self.revision = revision;
        Ok(())
    }

    pub(crate) fn advance_revision(&mut self, revision: u64) -> Result<()> {
        let expected_revision = self.revision.checked_add(1).ok_or_else(|| {
            Error::InvalidState("state revision exceeded the u64 range".to_owned())
        })?;
        if revision != expected_revision {
            return Err(Error::InvalidState(format!(
                "expected revision {}, found {revision}",
                expected_revision
            )));
        }
        self.revision = revision;
        Ok(())
    }

    fn validate(&self, entry: &Transition) -> Result<()> {
        if let Some(operation_id) = entry.operation_id() {
            ensure_nonempty(operation_id, "operation ID")?;
        }
        match entry {
            Transition::ExecutionAdvanced { operation_id, .. } => {
                self.ensure_prior_operations_terminal(operation_id)?;
                let operation = self.pending_operation(operation_id)?;
                if operation
                    .steps
                    .values()
                    .any(|step| matches!(step.status, StepStatus::EffectPending))
                {
                    return Err(Error::InvalidState(format!(
                        "operation `{operation_id}` cannot advance past an unsettled effect"
                    )));
                }
                for output in &operation.boundary_outputs {
                    if let Some(index) = output.model_call_index {
                        let step_id = format!("model-{index}");
                        if operation
                            .steps
                            .get(&step_id)
                            .is_some_and(|step| matches!(step.status, StepStatus::Completed(_)))
                            && operation.boundary_output_receipts[&output.message_id]
                                .confirmed_model_call_index
                                != Some(index)
                        {
                            return Err(Error::InvalidState(format!(
                                "operation `{operation_id}` cannot retire unconfirmed output at `{step_id}`"
                            )));
                        }
                    }
                }
            }
            Transition::OperationAccepted { operation_id, .. } => {
                if self.operations.contains_key(operation_id) {
                    return Err(Error::InvalidState(format!(
                        "operation `{operation_id}` was accepted more than once"
                    )));
                }
            }
            Transition::StepStarted {
                operation_id,
                step_id,
                kind,
                input,
            } => {
                ensure_nonempty(step_id, "step ID")?;
                ensure_nonempty(kind, "step kind")?;
                self.ensure_prior_operations_terminal(operation_id)?;
                let operation = self.pending_operation(operation_id)?;
                if kind == "model_call"
                    && step_id
                        .strip_prefix("model-")
                        .and_then(|id| id.parse::<u32>().ok())
                        .is_some_and(|index| index <= operation.retired_model_calls)
                {
                    return Err(Error::InvalidState(
                        "cannot execute a retired model batch".into(),
                    ));
                }
                if let Some(step) = operation.steps.get(step_id) {
                    if step.kind != *kind || step.input != *input {
                        return Err(Error::InvalidState(format!(
                            "step `{step_id}` in operation `{operation_id}` changed definition"
                        )));
                    }
                    if matches!(step.status, StepStatus::Completed(_)) {
                        return Err(Error::InvalidState(format!(
                            "settled step `{step_id}` in operation `{operation_id}` restarted"
                        )));
                    }
                    if step.attempts == u32::MAX {
                        return Err(Error::InvalidState(format!(
                            "step `{step_id}` in operation `{operation_id}` exceeded the attempt counter range"
                        )));
                    }
                }
            }
            Transition::StepCompleted {
                operation_id,
                step_id,
                output: _,
                late_response_id,
            } => {
                if let Some(response_id) = late_response_id {
                    let index = step_id
                        .strip_prefix("model-")
                        .and_then(|n| n.parse::<u32>().ok());
                    if !operation_id.starts_with("late-continuation:")
                        || index.is_none_or(|n| n == 0)
                        || response_id.trim().is_empty()
                    {
                        return Err(Error::InvalidState(
                            "invalid late wake model receipt".into(),
                        ));
                    }
                }
                ensure_nonempty(step_id, "step ID")?;
                self.ensure_prior_operations_terminal(operation_id)?;
                let operation = self.pending_operation(operation_id)?;
                let step = operation.steps.get(step_id).ok_or_else(|| {
                    Error::InvalidState(format!(
                        "step `{step_id}` in operation `{operation_id}` completed before start"
                    ))
                })?;
                if late_response_id.is_some() && step.kind != "model_call" {
                    return Err(Error::InvalidState(
                        "late wake receipt requires a model step".into(),
                    ));
                }
                match &step.status {
                    StepStatus::EffectPending => {}
                    StepStatus::Completed(_) => {
                        return Err(Error::InvalidState(format!(
                            "step `{step_id}` in operation `{operation_id}` completed more than once"
                        )));
                    }
                }
            }
            Transition::SteerAccepted {
                operation_id,
                steer_index,
                accepted_after_model_call_index,
                input: _,
                message_id,
            } => {
                self.ensure_prior_operations_terminal(operation_id)?;
                if *accepted_after_model_call_index == 0 {
                    return Err(Error::InvalidState(format!(
                        "steer {steer_index} in operation `{operation_id}` has an invalid acceptance boundary"
                    )));
                }
                let operation = self.pending_operation(operation_id)?;
                if message_id
                    .as_ref()
                    .is_some_and(|id| id.is_empty() || operation.steer_receipts.contains_key(id))
                {
                    return Err(Error::InvalidState(
                        "steer identity was already accepted or is empty".into(),
                    ));
                }
                let expected = u32::try_from(operation.steers.len())
                    .ok()
                    .and_then(|length| length.checked_add(operation.retired_steers)?.checked_add(1))
                    .ok_or_else(|| {
                        Error::InvalidState(format!(
                            "operation `{operation_id}` exceeded the steer counter range"
                        ))
                    })?;
                if *steer_index != expected {
                    return Err(Error::InvalidState(format!(
                        "operation `{operation_id}` expected steer {expected}, found {steer_index}"
                    )));
                }
            }
            Transition::SteerWithdrawn {
                operation_id,
                steer_index,
            } => {
                self.ensure_prior_operations_terminal(operation_id)?;
                let operation = self.pending_operation(operation_id)?;
                if steer_index
                    .checked_sub(operation.retired_steers)
                    .and_then(|index| usize::try_from(index).ok())
                    != Some(operation.steers.len())
                    || !operation
                        .steers
                        .last()
                        .is_some_and(|steer| steer.model_call_index.is_none())
                {
                    return Err(Error::InvalidState(format!(
                        "steer {steer_index} in operation `{operation_id}` is not the latest unbound steer"
                    )));
                }
            }
            Transition::SteerBound {
                operation_id,
                steer_index,
                model_call_index,
            } => {
                self.ensure_prior_operations_terminal(operation_id)?;
                if *model_call_index == 0 {
                    return Err(Error::InvalidState(format!(
                        "steer {steer_index} in operation `{operation_id}` has an invalid model boundary"
                    )));
                }
                let operation = self.pending_operation(operation_id)?;
                let steer = steer_index
                    .checked_sub(operation.retired_steers)
                    .and_then(|index| index.checked_sub(1))
                    .and_then(|index| usize::try_from(index).ok())
                    .and_then(|index| operation.steers.get(index))
                    .ok_or_else(|| {
                        Error::InvalidState(format!(
                            "steer {steer_index} in operation `{operation_id}` was bound before acceptance"
                        ))
                    })?;
                if *model_call_index <= steer.accepted_after_model_call_index {
                    return Err(Error::InvalidState(format!(
                        "steer {steer_index} in operation `{operation_id}` cannot bind to model call {model_call_index} after acceptance at {}",
                        steer.accepted_after_model_call_index
                    )));
                }
                if steer.model_call_index.is_some() {
                    return Err(Error::InvalidState(format!(
                        "steer {steer_index} in operation `{operation_id}` was bound more than once"
                    )));
                }
                if *steer_index - operation.retired_steers > 1 {
                    let previous_index = usize::try_from(
                        *steer_index - operation.retired_steers - 2,
                    )
                    .map_err(|_| {
                        Error::InvalidState(format!(
                            "steer {steer_index} in operation `{operation_id}` has an invalid index"
                        ))
                    })?;
                    let previous = &operation.steers[previous_index];
                    let Some(previous_model_call_index) = previous.model_call_index else {
                        return Err(Error::InvalidState(format!(
                            "steer {steer_index} in operation `{operation_id}` was bound before an earlier steer"
                        )));
                    };
                    if *model_call_index < previous_model_call_index {
                        return Err(Error::InvalidState(format!(
                            "steer {steer_index} in operation `{operation_id}` moved before an earlier steer"
                        )));
                    }
                }
            }
            Transition::BoundaryOutputAccepted {
                operation_id,
                output_index,
                accepted_after_model_call_index: _,
                message_id,
                ..
            } => {
                self.ensure_prior_operations_terminal(operation_id)?;
                let operation = self.pending_operation(operation_id)?;
                let expected = u32::try_from(operation.boundary_outputs.len())
                    .ok()
                    .and_then(|n| n.checked_add(operation.retired_boundary_outputs))
                    .and_then(|n| n.checked_add(1))
                    .ok_or_else(|| Error::InvalidState("boundary output index overflow".into()))?;
                if message_id.is_empty()
                    || operation.boundary_output_receipts.contains_key(message_id)
                    || *output_index != expected
                {
                    return Err(Error::InvalidState(
                        "invalid or duplicate boundary output acceptance".into(),
                    ));
                }
            }
            Transition::BoundaryOutputBound {
                operation_id,
                output_index,
                model_call_index,
            } => {
                self.ensure_prior_operations_terminal(operation_id)?;
                let operation = self.pending_operation(operation_id)?;
                let index = output_index
                    .checked_sub(operation.retired_boundary_outputs)
                    .and_then(|index| index.checked_sub(1))
                    .and_then(|index| usize::try_from(index).ok())
                    .ok_or_else(|| {
                        Error::InvalidState("boundary output index was retired".into())
                    })?;
                let output = operation.boundary_outputs.get(index).ok_or_else(|| {
                    Error::InvalidState("boundary output was not accepted".into())
                })?;
                if *model_call_index <= output.accepted_after_model_call_index
                    || output.model_call_index.is_some()
                    || operation.boundary_outputs[..index].iter().any(|earlier| {
                        earlier
                            .model_call_index
                            .is_none_or(|bound| bound > *model_call_index)
                    })
                {
                    return Err(Error::InvalidState(
                        "invalid boundary output binding or FIFO order".into(),
                    ));
                }
            }
            Transition::BoundaryOutputConfirmed {
                operation_id,
                output_index,
                model_call_index,
                response_id,
            } => {
                self.ensure_prior_operations_terminal(operation_id)?;
                let operation = self.pending_operation(operation_id)?;
                let output = output_index
                    .checked_sub(operation.retired_boundary_outputs)
                    .and_then(|index| index.checked_sub(1))
                    .and_then(|index| usize::try_from(index).ok())
                    .and_then(|index| operation.boundary_outputs.get(index))
                    .ok_or_else(|| {
                        Error::InvalidState(
                            "boundary output was not retained for confirmation".into(),
                        )
                    })?;
                let step_id = format!("model-{model_call_index}");
                if response_id.is_empty()
                    || output.model_call_index != Some(*model_call_index)
                    || operation.boundary_output_receipts[&output.message_id]
                        .confirmed_model_call_index
                        .is_some()
                    || !operation.steps.get(&step_id).is_some_and(|step| {
                        step.kind == "model_call" && matches!(step.status, StepStatus::Completed(_))
                    })
                {
                    return Err(Error::InvalidState(
                        "boundary output confirmed without completed matching model step".into(),
                    ));
                }
            }
            Transition::OperationCompleted { operation_id, .. } => {
                self.ensure_prior_operations_terminal(operation_id)?;
                let operation = self.pending_operation(operation_id)?;
                if operation
                    .steps
                    .values()
                    .any(|step| !matches!(step.status, StepStatus::Completed(_)))
                {
                    return Err(Error::InvalidState(format!(
                        "operation `{operation_id}` completed with an unfinished step"
                    )));
                }
                ensure_completed_steers_consumed(operation_id, operation)?;
                ensure_completed_boundary_outputs_consumed(operation_id, operation)?;
            }
            Transition::OperationFailed { operation_id, .. } => {
                self.ensure_prior_operations_terminal(operation_id)?;
                self.pending_operation(operation_id)?;
            }
            Transition::OperationCancelled {
                operation_id,
                checkpoint,
            } => {
                let operation = self.pending_operation(operation_id)?;
                if checkpoint.is_some() {
                    self.ensure_prior_operations_terminal(operation_id)?;
                } else if operation.cancellation_requires_checkpoint() {
                    return Err(Error::InvalidState(format!(
                        "started operation `{operation_id}` was cancelled without a checkpoint"
                    )));
                }
            }
            Transition::CheckpointCommitted { .. } => {
                if let Some((pending_id, _)) = self.first_pending_operation() {
                    return Err(Error::InvalidState(format!(
                        "standalone checkpoint effect crossed pending operation `{pending_id}`"
                    )));
                }
            }
        }
        Ok(())
    }

    fn apply(&mut self, revision: u64, entry: Transition) -> Result<()> {
        match entry {
            Transition::ExecutionAdvanced {
                operation_id,
                continuation,
            } => {
                let operation = self.pending_operation_mut(&operation_id)?;
                operation.continuation = Some(continuation);
                operation.retire_steps();
            }
            Transition::OperationAccepted {
                operation_id,
                input,
            } => {
                self.operations.insert(
                    operation_id,
                    OperationState {
                        steer_receipts: BTreeMap::new(),
                        boundary_output_receipts: BTreeMap::new(),
                        boundary_outputs: Vec::new(),
                        late_model_response: None,
                        retired_boundary_outputs: 0,
                        continuation: None,
                        retired_model_calls: 0,
                        retired_steers: 0,
                        input,
                        status: OperationStatus::Pending,
                        steps: BTreeMap::new(),
                        steers: Vec::new(),
                        accepted_order: revision,
                    },
                );
            }
            Transition::StepStarted {
                operation_id,
                step_id,
                kind,
                input,
            } => {
                let operation = self.pending_operation_mut(&operation_id)?;
                if let Some(step) = operation.steps.get_mut(&step_id) {
                    step.attempts = step.attempts.checked_add(1).ok_or_else(|| {
                        Error::InvalidState(format!(
                            "step `{step_id}` in operation `{operation_id}` exceeded the attempt counter range"
                        ))
                    })?;
                } else {
                    operation.steps.insert(
                        step_id,
                        StepState {
                            kind,
                            input,
                            status: StepStatus::EffectPending,
                            attempts: 1,
                        },
                    );
                }
            }
            Transition::StepCompleted {
                operation_id,
                step_id,
                output,
                late_response_id,
            } => {
                let operation = self.pending_operation_mut(&operation_id)?;
                if let Some(response_id) = late_response_id {
                    let index = step_id
                        .strip_prefix("model-")
                        .unwrap()
                        .parse::<u32>()
                        .unwrap();
                    if operation.late_model_response.is_none() {
                        operation.late_model_response = Some((index, response_id));
                    }
                }
                let step = operation.steps.get_mut(&step_id).ok_or_else(|| {
                    Error::InvalidState(format!(
                        "step `{step_id}` in operation `{operation_id}` completed before start"
                    ))
                })?;
                step.status = StepStatus::Completed(output);
            }
            Transition::BoundaryOutputAccepted {
                operation_id,
                output_index,
                accepted_after_model_call_index,
                message_id,
                input,
            } => {
                let operation = self.pending_operation_mut(&operation_id)?;
                operation.boundary_output_receipts.insert(
                    message_id.clone(),
                    BoundaryOutputReceipt {
                        input_key: input.key.to_string(),
                        call_id: input
                            .decode::<nanocodex_agent::execution::ExecutionBoundaryOutput>()
                            .ok()
                            .and_then(|output| match output {
                                nanocodex_agent::execution::ExecutionBoundaryOutput::TerminalOutput {
                                    call_id,
                                    ..
                                } if !call_id.is_empty() => Some(call_id),
                                _ => None,
                            }),
                        index: output_index,
                        bound_model_call_index: None,
                        confirmed_model_call_index: None,
                        response_id: None,
                        discarded: false,
                    },
                );
                operation.boundary_outputs.push(BoundaryOutputState {
                    message_id,
                    input,
                    accepted_after_model_call_index,
                    model_call_index: None,
                });
            }
            Transition::BoundaryOutputBound {
                operation_id,
                output_index,
                model_call_index,
            } => {
                let operation = self.pending_operation_mut(&operation_id)?;
                let output = &mut operation.boundary_outputs
                    [(output_index - operation.retired_boundary_outputs - 1) as usize];
                output.model_call_index = Some(model_call_index);
                operation
                    .boundary_output_receipts
                    .get_mut(&output.message_id)
                    .unwrap()
                    .bound_model_call_index = Some(model_call_index);
            }
            Transition::BoundaryOutputConfirmed {
                operation_id,
                output_index,
                model_call_index,
                response_id,
            } => {
                let operation = self.pending_operation_mut(&operation_id)?;
                let output = &operation.boundary_outputs
                    [(output_index - operation.retired_boundary_outputs - 1) as usize];
                let receipt = operation
                    .boundary_output_receipts
                    .get_mut(&output.message_id)
                    .unwrap();
                receipt.confirmed_model_call_index = Some(model_call_index);
                receipt.response_id = Some(response_id);
            }
            Transition::SteerAccepted {
                operation_id,
                steer_index,
                accepted_after_model_call_index,
                input,
                message_id,
            } => {
                let operation = self.pending_operation_mut(&operation_id)?;
                if let Some(id) = &message_id {
                    operation.steer_receipts.insert(
                        id.clone(),
                        IdentifiedSteerReceipt {
                            input_key: input.key.to_string(),
                            index: steer_index,
                            withdrawn: false,
                        },
                    );
                }
                operation.steers.push(SteerState {
                    message_id,
                    input,
                    accepted_after_model_call_index,
                    model_call_index: None,
                });
            }
            Transition::SteerBound {
                operation_id,
                steer_index,
                model_call_index,
            } => {
                let operation = self.pending_operation_mut(&operation_id)?;
                let index =
                    usize::try_from(steer_index - operation.retired_steers - 1).map_err(|_| {
                        Error::InvalidState(format!(
                            "steer {steer_index} in operation `{operation_id}` has an invalid index"
                        ))
                    })?;
                operation.steers[index].model_call_index = Some(model_call_index);
            }
            Transition::SteerWithdrawn { operation_id, .. } => {
                let operation = self.pending_operation_mut(&operation_id)?;
                if let Some(steer) = operation.steers.pop()
                    && let Some(id) = steer.message_id
                    && let Some(receipt) = operation.steer_receipts.get_mut(&id)
                {
                    receipt.withdrawn = true;
                }
            }
            Transition::OperationCompleted {
                operation_id,
                checkpoint,
                output,
            } => {
                let operation = self.pending_operation_mut(&operation_id)?;
                operation.continuation.take();
                operation.retire_steps();
                operation.status = OperationStatus::Completed {
                    checkpoint: checkpoint.clone(),
                    output,
                };
                self.latest_checkpoint = Some((revision, checkpoint));
            }
            Transition::OperationFailed {
                operation_id,
                checkpoint,
                error,
            } => {
                let operation = self.pending_operation_mut(&operation_id)?;
                if operation.continuation.take().is_some() {
                    operation.retire_steps();
                }
                for output in &operation.boundary_outputs {
                    if let Some(receipt) = operation
                        .boundary_output_receipts
                        .get_mut(&output.message_id)
                        && receipt.confirmed_model_call_index.is_none()
                    {
                        receipt.discarded = true;
                    }
                }
                operation.boundary_outputs.clear();
                operation.status = OperationStatus::Failed {
                    checkpoint: checkpoint.clone(),
                    error,
                };
                self.latest_checkpoint = Some((revision, checkpoint));
            }
            Transition::OperationCancelled {
                operation_id,
                checkpoint,
            } => {
                let operation = self.pending_operation_mut(&operation_id)?;
                if operation.continuation.take().is_some() {
                    operation.retire_steps();
                }
                for output in &operation.boundary_outputs {
                    if let Some(receipt) = operation
                        .boundary_output_receipts
                        .get_mut(&output.message_id)
                        && receipt.confirmed_model_call_index.is_none()
                    {
                        receipt.discarded = true;
                    }
                }
                operation.boundary_outputs.clear();
                operation.status = OperationStatus::Cancelled {
                    checkpoint: checkpoint.clone(),
                };
                if let Some(checkpoint) = checkpoint {
                    self.latest_checkpoint = Some((revision, checkpoint));
                }
            }
            Transition::CheckpointCommitted { checkpoint } => {
                self.latest_checkpoint = Some((revision, checkpoint));
            }
        }
        Ok(())
    }

    fn pending_operation_mut(&mut self, operation_id: &str) -> Result<&mut OperationState> {
        let operation = self.operations.get_mut(operation_id).ok_or_else(|| {
            Error::InvalidState(format!("operation `{operation_id}` was not accepted"))
        })?;
        if operation.status.is_terminal() {
            return Err(Error::InvalidState(format!(
                "terminal operation `{operation_id}` was changed"
            )));
        }
        Ok(operation)
    }

    fn pending_operation(&self, operation_id: &str) -> Result<&OperationState> {
        let operation = self.operations.get(operation_id).ok_or_else(|| {
            Error::InvalidState(format!("operation `{operation_id}` was not accepted"))
        })?;
        if operation.status.is_terminal() {
            return Err(Error::InvalidState(format!(
                "terminal operation `{operation_id}` was changed"
            )));
        }
        Ok(operation)
    }

    fn ensure_prior_operations_terminal(&self, operation_id: &str) -> Result<()> {
        let operation = self.operations.get(operation_id).ok_or_else(|| {
            Error::InvalidState(format!("operation `{operation_id}` was not accepted"))
        })?;
        if let Some((pending_id, _)) = self.operations.iter().find(|(id, candidate)| {
            candidate.accepted_order < operation.accepted_order
                && !candidate.status.is_terminal()
                && id.as_str() != operation_id
        }) {
            return Err(Error::InvalidState(format!(
                "operation `{operation_id}` completed before `{pending_id}`"
            )));
        }
        Ok(())
    }
}

fn ensure_completed_boundary_outputs_consumed(
    operation_id: &str,
    operation: &OperationState,
) -> Result<()> {
    for output in &operation.boundary_outputs {
        let receipt = &operation.boundary_output_receipts[&output.message_id];
        if receipt.confirmed_model_call_index != output.model_call_index
            || receipt.confirmed_model_call_index.is_none()
        {
            return Err(Error::InvalidState(format!(
                "operation `{operation_id}` completed with output not confirmed by a durable model step"
            )));
        }
    }
    Ok(())
}

fn ensure_completed_steers_consumed(operation_id: &str, operation: &OperationState) -> Result<()> {
    for (offset, steer) in operation.steers.iter().enumerate() {
        let steer_index = offset + 1 + operation.retired_steers as usize;
        let model_call_index = steer.model_call_index.ok_or_else(|| {
            Error::InvalidState(format!(
                "operation `{operation_id}` completed with unbound steer {steer_index}"
            ))
        })?;
        let step_id = format!("model-{model_call_index}");
        let consumed = model_call_index <= operation.retired_model_calls
            || operation.steps.get(&step_id).is_some_and(|step| {
                step.kind == "model_call" && matches!(step.status, StepStatus::Completed(_))
            });
        if !consumed {
            return Err(Error::InvalidState(format!(
                "operation `{operation_id}` completed before steer {steer_index} was consumed by `{step_id}`"
            )));
        }
    }
    Ok(())
}

impl Transition {
    fn operation_id(&self) -> Option<&str> {
        match self {
            Self::ExecutionAdvanced { operation_id, .. }
            | Self::OperationAccepted { operation_id, .. }
            | Self::StepStarted { operation_id, .. }
            | Self::StepCompleted { operation_id, .. }
            | Self::BoundaryOutputAccepted { operation_id, .. }
            | Self::BoundaryOutputBound { operation_id, .. }
            | Self::BoundaryOutputConfirmed { operation_id, .. }
            | Self::SteerAccepted { operation_id, .. }
            | Self::SteerBound { operation_id, .. }
            | Self::SteerWithdrawn { operation_id, .. }
            | Self::OperationCompleted { operation_id, .. }
            | Self::OperationFailed { operation_id, .. }
            | Self::OperationCancelled { operation_id, .. } => Some(operation_id),
            Self::CheckpointCommitted { .. } => None,
        }
    }
}

fn ensure_nonempty(value: &str, name: &str) -> Result<()> {
    if value.trim().is_empty() {
        return Err(Error::InvalidState(format!("{name} must not be empty")));
    }
    Ok(())
}

#[cfg(test)]
mod continuation_tests {
    use super::*;

    #[test]
    fn a_long_turn_retires_consumed_steers_without_reusing_their_indices() -> Result<()> {
        let mut state = DurableState::default();
        let id = "turn".to_owned();
        let payload = EncodedPayload::encode(&"context")?;
        state.apply_transition(
            1,
            Transition::OperationAccepted {
                operation_id: id.clone(),
                input: payload.clone(),
            },
        )?;
        for model_call in 1..=257 {
            let mut apply = |entry| state.apply_transition(state.revision() + 1, entry);
            if model_call > 1 {
                apply(Transition::SteerBound {
                    operation_id: id.clone(),
                    steer_index: model_call - 1,
                    model_call_index: model_call,
                })?;
            }
            apply(Transition::StepStarted {
                operation_id: id.clone(),
                step_id: format!("model-{model_call}"),
                kind: "model_call".into(),
                input: payload.clone(),
            })?;
            if model_call <= 256 {
                apply(Transition::SteerAccepted {
                    message_id: None,
                    operation_id: id.clone(),
                    steer_index: model_call,
                    accepted_after_model_call_index: model_call,
                    input: payload.clone(),
                })?;
            }
            apply(Transition::StepCompleted {
                operation_id: id.clone(),
                step_id: format!("model-{model_call}"),
                output: payload.clone(),
                late_response_id: None,
            })?;
            apply(Transition::ExecutionAdvanced {
                operation_id: id.clone(),
                continuation: payload.clone(),
            })?;
            let operation = state.operation(&id).unwrap();
            assert_eq!(operation.retired_steers, model_call - 1);
            assert_eq!(operation.steers.len(), usize::from(model_call <= 256));
            assert!(operation.steps.is_empty());
        }
        Ok(())
    }

    #[test]
    fn advancing_preserves_steer_consumption_and_rejects_pending_or_retired_work() -> Result<()> {
        let mut state = DurableState::default();
        let id = "turn".to_owned();
        let payload = EncodedPayload::encode(&"state")?;
        let mut apply = |entry| state.apply_transition(state.revision() + 1, entry);
        apply(Transition::OperationAccepted {
            operation_id: id.clone(),
            input: payload.clone(),
        })?;
        apply(Transition::StepStarted {
            operation_id: id.clone(),
            step_id: "model-1".into(),
            kind: "model_call".into(),
            input: payload.clone(),
        })?;
        apply(Transition::SteerAccepted {
            message_id: None,
            operation_id: id.clone(),
            steer_index: 1,
            accepted_after_model_call_index: 1,
            input: payload.clone(),
        })?;
        apply(Transition::StepCompleted {
            operation_id: id.clone(),
            step_id: "model-1".into(),
            output: payload.clone(),
            late_response_id: None,
        })?;
        apply(Transition::ExecutionAdvanced {
            operation_id: id.clone(),
            continuation: payload.clone(),
        })?;
        apply(Transition::SteerBound {
            operation_id: id.clone(),
            steer_index: 1,
            model_call_index: 2,
        })?;
        apply(Transition::StepStarted {
            operation_id: id.clone(),
            step_id: "model-2".into(),
            kind: "model_call".into(),
            input: payload.clone(),
        })?;
        assert!(
            apply(Transition::ExecutionAdvanced {
                operation_id: id.clone(),
                continuation: payload.clone()
            })
            .is_err()
        );
        apply(Transition::StepCompleted {
            operation_id: id.clone(),
            step_id: "model-2".into(),
            output: payload.clone(),
            late_response_id: None,
        })?;
        apply(Transition::ExecutionAdvanced {
            operation_id: id.clone(),
            continuation: payload.clone(),
        })?;
        assert!(
            apply(Transition::StepStarted {
                operation_id: id.clone(),
                step_id: "model-1".into(),
                kind: "model_call".into(),
                input: payload.clone()
            })
            .is_err()
        );
        apply(Transition::OperationCompleted {
            operation_id: id.clone(),
            checkpoint: payload.clone(),
            output: payload,
        })?;
        let operation = state.operation(&id).unwrap();
        assert_eq!(operation.retired_model_calls, 2);
        assert!(operation.continuation.is_none());
        assert!(operation.steps.is_empty());
        Ok(())
    }
}

#[cfg(test)]
mod withdrawal_tests {
    use super::*;

    #[test]
    fn withdrawal_is_replayable_and_rejects_consumed_or_nonlatest_steers() -> Result<()> {
        let mut state = DurableState::default();
        let payload = EncodedPayload::encode(&"input")?;
        let mut transitions = Vec::new();
        let mut apply = |entry: Transition| {
            state.apply_transition(state.revision() + 1, entry.clone())?;
            transitions.push(entry);
            Ok::<_, Error>(())
        };
        apply(Transition::OperationAccepted {
            operation_id: "turn".into(),
            input: payload.clone(),
        })?;
        for steer_index in 1..=2 {
            apply(Transition::SteerAccepted {
                message_id: None,
                operation_id: "turn".into(),
                steer_index,
                accepted_after_model_call_index: 1,
                input: payload.clone(),
            })?;
        }
        assert!(
            apply(Transition::SteerWithdrawn {
                operation_id: "turn".into(),
                steer_index: 1
            })
            .is_err()
        );
        apply(Transition::SteerWithdrawn {
            operation_id: "turn".into(),
            steer_index: 2,
        })?;
        apply(Transition::SteerBound {
            operation_id: "turn".into(),
            steer_index: 1,
            model_call_index: 2,
        })?;
        assert!(
            apply(Transition::SteerWithdrawn {
                operation_id: "turn".into(),
                steer_index: 1
            })
            .is_err()
        );
        let mut replay = DurableState::default();
        for entry in transitions {
            let encoded = serde_json::to_string(&entry)?;
            replay.apply_transition(replay.revision() + 1, serde_json::from_str(&encoded)?)?;
        }
        assert_eq!(replay.operation("turn").unwrap().steers.len(), 1);
        assert_eq!(
            replay.operation("turn").unwrap().steers[0].model_call_index,
            Some(2)
        );
        Ok(())
    }

    #[test]
    fn withdrawal_uses_absolute_indices_after_consumed_steers_are_retired() -> Result<()> {
        let mut state = DurableState::default();
        let payload = EncodedPayload::encode(&"input")?;
        let mut transitions = Vec::new();
        let mut apply = |entry: Transition| {
            state.apply_transition(state.revision() + 1, entry.clone())?;
            transitions.push(entry);
            Ok::<_, Error>(())
        };
        apply(Transition::OperationAccepted {
            operation_id: "turn".into(),
            input: payload.clone(),
        })?;
        apply(Transition::SteerAccepted {
            message_id: None,
            operation_id: "turn".into(),
            steer_index: 1,
            accepted_after_model_call_index: 1,
            input: payload.clone(),
        })?;
        apply(Transition::SteerBound {
            operation_id: "turn".into(),
            steer_index: 1,
            model_call_index: 2,
        })?;
        apply(Transition::StepStarted {
            operation_id: "turn".into(),
            step_id: "model-2".into(),
            kind: "model_call".into(),
            input: payload.clone(),
        })?;
        apply(Transition::StepCompleted {
            operation_id: "turn".into(),
            step_id: "model-2".into(),
            output: payload.clone(),
            late_response_id: None,
        })?;
        apply(Transition::ExecutionAdvanced {
            operation_id: "turn".into(),
            continuation: payload.clone(),
        })?;
        for steer_index in 2..=3 {
            apply(Transition::SteerAccepted {
                message_id: None,
                operation_id: "turn".into(),
                steer_index,
                accepted_after_model_call_index: 2,
                input: payload.clone(),
            })?;
        }
        assert!(
            apply(Transition::SteerWithdrawn {
                operation_id: "turn".into(),
                steer_index: 2
            })
            .is_err()
        );
        apply(Transition::SteerWithdrawn {
            operation_id: "turn".into(),
            steer_index: 3,
        })?;
        apply(Transition::SteerBound {
            operation_id: "turn".into(),
            steer_index: 2,
            model_call_index: 3,
        })?;
        assert!(
            apply(Transition::SteerWithdrawn {
                operation_id: "turn".into(),
                steer_index: 2
            })
            .is_err()
        );
        let mut replay = DurableState::default();
        for entry in transitions {
            let encoded = serde_json::to_string(&entry)?;
            replay.apply_transition(replay.revision() + 1, serde_json::from_str(&encoded)?)?;
        }
        assert_eq!(replay.operation("turn").unwrap().retired_steers, 1);
        assert_eq!(replay.operation("turn").unwrap().steers.len(), 1);
        assert_eq!(
            replay.operation("turn").unwrap().steers[0].model_call_index,
            Some(3)
        );
        Ok(())
    }
}

#[cfg(test)]
mod boundary_output_lifecycle_tests {
    use super::*;

    #[test]
    fn ordered_outputs_bind_to_one_boundary_and_retire_only_with_persisted_conversation()
    -> Result<()> {
        let mut state = DurableState::default();
        let payload = EncodedPayload::encode(&"terminal")?;
        fn apply(state: &mut DurableState, entry: Transition) -> Result<()> {
            state.apply_transition(state.revision() + 1, entry)
        }
        apply(
            &mut state,
            Transition::OperationAccepted {
                operation_id: "turn".into(),
                input: payload.clone(),
            },
        )?;
        for index in 1..=2 {
            apply(
                &mut state,
                Transition::BoundaryOutputAccepted {
                    operation_id: "turn".into(),
                    output_index: index,
                    accepted_after_model_call_index: 1,
                    message_id: format!("msg-{index}"),
                    input: payload.clone(),
                },
            )?;
        }
        assert!(
            apply(
                &mut state,
                Transition::BoundaryOutputBound {
                    operation_id: "turn".into(),
                    output_index: 2,
                    model_call_index: 2
                }
            )
            .is_err()
        );
        for index in 1..=2 {
            apply(
                &mut state,
                Transition::BoundaryOutputBound {
                    operation_id: "turn".into(),
                    output_index: index,
                    model_call_index: 2,
                },
            )?;
        }
        assert!(
            apply(
                &mut state,
                Transition::OperationCompleted {
                    operation_id: "turn".into(),
                    checkpoint: payload.clone(),
                    output: payload.clone()
                }
            )
            .is_err()
        );
        apply(
            &mut state,
            Transition::StepStarted {
                operation_id: "turn".into(),
                step_id: "model-2".into(),
                kind: "model_call".into(),
                input: payload.clone(),
            },
        )?;
        apply(
            &mut state,
            Transition::StepCompleted {
                operation_id: "turn".into(),
                step_id: "model-2".into(),
                output: payload.clone(),
                late_response_id: None,
            },
        )?;
        assert_eq!(state.operation("turn").unwrap().boundary_outputs.len(), 2);
        assert!(
            apply(
                &mut state,
                Transition::ExecutionAdvanced {
                    operation_id: "turn".into(),
                    continuation: payload.clone(),
                }
            )
            .is_err()
        );
        for index in 1..=2 {
            apply(
                &mut state,
                Transition::BoundaryOutputConfirmed {
                    operation_id: "turn".into(),
                    output_index: index,
                    model_call_index: 2,
                    response_id: "resp-2".into(),
                },
            )?;
        }
        apply(
            &mut state,
            Transition::ExecutionAdvanced {
                operation_id: "turn".into(),
                continuation: payload.clone(),
            },
        )?;
        let operation = state.operation("turn").unwrap();
        assert!(operation.boundary_outputs.is_empty());
        assert_eq!(operation.retired_boundary_outputs, 2);
        assert_eq!(operation.boundary_output_receipts["msg-1"].index, 1);
        assert!(
            apply(
                &mut state,
                Transition::BoundaryOutputBound {
                    operation_id: "turn".into(),
                    output_index: 1,
                    model_call_index: 3
                }
            )
            .is_err()
        );
        apply(
            &mut state,
            Transition::OperationCompleted {
                operation_id: "turn".into(),
                checkpoint: payload.clone(),
                output: payload,
            },
        )?;
        Ok(())
    }

    #[test]
    fn completion_retires_consumed_output_and_cancel_discards_live_body_but_keeps_receipt()
    -> Result<()> {
        for cancel in [false, true] {
            let mut state = DurableState::default();
            let payload = EncodedPayload::encode(&"terminal")?;
            fn apply(state: &mut DurableState, entry: Transition) -> Result<()> {
                state.apply_transition(state.revision() + 1, entry)
            }
            apply(
                &mut state,
                Transition::OperationAccepted {
                    operation_id: "turn".into(),
                    input: payload.clone(),
                },
            )?;
            apply(
                &mut state,
                Transition::BoundaryOutputAccepted {
                    operation_id: "turn".into(),
                    output_index: 1,
                    accepted_after_model_call_index: 1,
                    message_id: "msg".into(),
                    input: payload.clone(),
                },
            )?;
            if !cancel {
                apply(
                    &mut state,
                    Transition::BoundaryOutputBound {
                        operation_id: "turn".into(),
                        output_index: 1,
                        model_call_index: 2,
                    },
                )?;
                apply(
                    &mut state,
                    Transition::StepStarted {
                        operation_id: "turn".into(),
                        step_id: "model-2".into(),
                        kind: "model_call".into(),
                        input: payload.clone(),
                    },
                )?;
                apply(
                    &mut state,
                    Transition::StepCompleted {
                        operation_id: "turn".into(),
                        step_id: "model-2".into(),
                        output: payload.clone(),
                        late_response_id: None,
                    },
                )?;
                apply(
                    &mut state,
                    Transition::BoundaryOutputConfirmed {
                        operation_id: "turn".into(),
                        output_index: 1,
                        model_call_index: 2,
                        response_id: "resp-2".into(),
                    },
                )?;
                apply(
                    &mut state,
                    Transition::OperationCompleted {
                        operation_id: "turn".into(),
                        checkpoint: payload.clone(),
                        output: payload.clone(),
                    },
                )?;
            } else {
                apply(
                    &mut state,
                    Transition::OperationCancelled {
                        operation_id: "turn".into(),
                        checkpoint: Some(payload.clone()),
                    },
                )?;
            }
            let operation = state.operation("turn").unwrap();
            assert!(operation.boundary_outputs.is_empty());
            assert_eq!(operation.boundary_output_receipts["msg"].index, 1);
            if !cancel {
                assert_eq!(operation.retired_boundary_outputs, 1);
            }
        }
        Ok(())
    }
}
